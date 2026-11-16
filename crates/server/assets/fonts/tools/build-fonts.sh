#!/bin/bash
# usage: build-fonts.sh <python> <srcdir> <outdir> <rename.py>
set -euo pipefail
PY="$1"; SRC="$2"; O="$3"; RENAME="$4"
U="U+0020-007E,U+00A0-017F,U+2013,U+2014,U+2018,U+2019,U+201C,U+201D,U+2022,U+2026,U+2212"
"$PY" -I -m fontTools varLib.instancer "$SRC/Bricolage.ttf" opsz=24 wdth=100 wght=800 -o "$O/bg-800.ttf"
"$PY" -I -m fontTools varLib.instancer "$SRC/SourceSans3.ttf" wght=700 -o "$O/ss-700.ttf"
"$PY" -I "$RENAME" "$O/ss-700.ttf" "$O/ss-700-renamed.ttf" "Autobracket Text" "Bold"
for pair in "bg-800.ttf:bricolage-grotesque-800.woff2" "ss-700-renamed.ttf:autobracket-text-700.woff2"; do
  "$PY" -I -m fontTools subset "$O/${pair%%:*}" --unicodes="$U" \
    --layout-features=kern,mark,mkmk,ccmp,locl,tnum,lnum --no-hinting --desubroutinize \
    --notdef-outline --name-IDs=0,1,2,3,4,5,6,13,14 --flavor=woff2 \
    --output-file="$O/${pair##*:}"
done
