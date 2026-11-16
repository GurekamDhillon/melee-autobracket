#!/bin/sh
# Stands in for the `melee` engine in tests.
#   melee version   prints a version line.
#   melee ...       records its arguments (one per line) to $MELEE_SLP_DIR/argv, prints the
#                   host's MATCH CODE line on stderr, sleeps 1 s, writes one replay, exits 0.
set -eu

if [ "${1:-}" = "version" ]; then
    echo "melee (fake) 0.0.1 test build"
    echo "second line is ignored"
    exit 0
fi

: > "$MELEE_SLP_DIR/argv"
for arg in "$@"; do
    printf '%s\n' "$arg" >> "$MELEE_SLP_DIR/argv"
done

echo "play: netplay host: MATCH CODE K7QX (give it to the other player)" >&2
sleep 1
printf 'fake replay' > "$MELEE_SLP_DIR/Game_20261115T200000.slp"
exit 0
