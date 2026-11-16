#!/bin/sh
# Stands in for the `melee` engine in tests.
#   melee version   prints a version line.
#   melee ...       records its arguments (one per line) to $MELEE_SLP_DIR/argv, what its
#                   stderr is connected to to $MELEE_SLP_DIR/stderr-target, and its pid and
#                   process group to $MELEE_SLP_DIR/pid and .../pgid. Then, by first argument:
#     slow          prints a line on stderr, sleeps 2 s, prints another, writes one replay.
#     spawner       like slow, but first starts a background `sleep 30` of its own and
#                   records its pid to $MELEE_SLP_DIR/bgpid.
#     quick         prints the host's MATCH CODE line on stderr, writes one replay, exits.
#     sleepy        prints the MATCH CODE line, sleeps 30 s, writes one replay.
#     no-opponent   prints the MATCH CODE line and the engine's "no opponent joined" line,
#                   exits 4 without a replay.
#     exit4         exits 4 without that line and without a replay.
#     line-exit1    prints the "no opponent joined" line, exits 1 without a replay.
#     fail          exits 1 without a replay.
#     cwd           writes `stray.txt` to its current directory, then one replay.
#     (otherwise)   prints the MATCH CODE line, sleeps 1 s, writes one replay.
# Modes not listed with another exit status exit 0.
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
readlink "/proc/$$/fd/2" > "$MELEE_SLP_DIR/stderr-target" || true
echo "$$" > "$MELEE_SLP_DIR/pid"
cut -d' ' -f5 "/proc/$$/stat" > "$MELEE_SLP_DIR/pgid" 2>/dev/null || true

replay="$MELEE_SLP_DIR/Game_20261115T200000.slp"
code_line="play: netplay host: MATCH CODE K7QX (give it to the other player)"
case "${1:-}" in
    slow)
        echo "fake: first line" >&2
        sleep 2
        echo "fake: second line" >&2
        printf 'fake replay' > "$replay"
        ;;
    spawner)
        sleep 30 &
        echo "$!" > "$MELEE_SLP_DIR/bgpid"
        echo "fake: first line" >&2
        sleep 2
        printf 'fake replay' > "$replay"
        ;;
    quick)
        echo "$code_line" >&2
        printf 'fake replay' > "$replay"
        ;;
    sleepy)
        echo "$code_line" >&2
        sleep 30
        printf 'fake replay' > "$replay"
        ;;
    no-opponent)
        echo "$code_line" >&2
        echo "play: netplay: no opponent joined" >&2
        exit 4
        ;;
    exit4)
        echo "$code_line" >&2
        exit 4
        ;;
    line-exit1)
        echo "play: netplay: no opponent joined" >&2
        exit 1
        ;;
    fail)
        echo "fake: failing" >&2
        exit 1
        ;;
    cwd)
        printf 'stray' > stray.txt
        printf 'fake replay' > "$replay"
        ;;
    *)
        echo "$code_line" >&2
        sleep 1
        printf 'fake replay' > "$replay"
        ;;
esac
exit 0
