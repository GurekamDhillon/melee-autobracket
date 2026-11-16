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
#                   exits 5 without a replay.
#     exit5         prints the MATCH CODE line, exits 5 without that line or a replay.
#     line-exit0    prints the "no opponent joined" line, exits 0 without a replay.
#     line-exit1    prints the "no opponent joined" line, exits 1 without a replay.
#     disc-fail     prints a disc check error, exits 4 (the engine's disc check failed).
#     usage         prints an unknown-argument error, exits 2 (a usage error).
#     fail          exits 1 without a replay.
#     cwd           writes `stray.txt` to its current directory, then one replay.
#     game2         prints the MATCH CODE line, sleeps 2 s, writes one replay.
#     np-timeout    prints the MATCH CODE line and the installed engine's host line for
#                   nobody joining within --np-timeout, exits 1 without a replay.
#     no-room       prints the guest's line for an unknown match code, exits 1.
#     host-closed   prints the guest's line for a host that closed its room, exits 1.
#     bad-match     prints the line for an unknown `--match` fighter, exits 1.
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
        exit 5
        ;;
    exit5)
        echo "$code_line" >&2
        exit 5
        ;;
    line-exit0)
        echo "play: netplay: no opponent joined" >&2
        exit 0
        ;;
    disc-fail)
        echo "play: disc check failed: not an NTSC 1.02 image" >&2
        exit 4
        ;;
    usage)
        echo "fake: log line one" >&2
        echo "error: unexpected argument '--np-join-wait' found" >&2
        exit 2
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
    game2)
        echo "$code_line" >&2
        sleep 2
        printf 'fake replay' > "$replay"
        ;;
    np-timeout)
        echo "$code_line" >&2
        echo "play: netplay host: no connection after 60 s (Waiting for the guest...): nobody connected; check that the guest has your address and port, that UDP to it is allowed through the firewall / forwarded by the router, or use a match code (--np-server)" >&2
        exit 1
        ;;
    no-room)
        echo "play: netplay guest: no room with that code" >&2
        exit 1
        ;;
    host-closed)
        echo "play: netplay guest: The host closed the room" >&2
        exit 1
        ;;
    bad-match)
        echo "play: --match: unknown fighter 'bogus'" >&2
        exit 1
        ;;
    *)
        echo "$code_line" >&2
        sleep 1
        printf 'fake replay' > "$replay"
        ;;
esac
exit 0
