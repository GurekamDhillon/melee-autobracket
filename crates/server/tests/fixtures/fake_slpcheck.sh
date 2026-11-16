#!/bin/sh
# Stand-in for the game engine's command line, for tests.
#   fake_slpcheck.sh version
#   fake_slpcheck.sh slpcheck --slp FILE --iso ISO --csv OUT
# The behaviour of `slpcheck` is chosen by the replay's file name (without extension).
# Like the real engine, a disc it cannot boot is a CSV row `boot-error: ...` with exit 1
# (`nodisc`); exit 2 is kept for usage errors and an unreadable `.slp` (`usage`).

case "$1" in
version)
    echo "fake 0.0"
    echo "second line is ignored"
    exit 0
    ;;
slpcheck)
    shift
    ;;
*)
    echo "usage: fake_slpcheck.sh version | slpcheck --slp F --iso I --csv O" >&2
    exit 2
    ;;
esac

slp=""
csv=""
while [ $# -gt 0 ]; do
    case "$1" in
    --slp) slp="$2"; shift 2 ;;
    --iso) shift 2 ;;
    --csv) csv="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done
if [ -z "$slp" ] || [ -z "$csv" ]; then
    echo "missing --slp or --csv" >&2
    exit 2
fi

name=$(basename "$slp" .slp)
echo "engine: some log line on stdout"
echo "engine: some log line on stderr" >&2

row() {
    # status, last_frame, first_frame, first_port, first_field, engine_value, replay_value
    if [ ! -s "$csv" ]; then
        echo "file,status,replay_frames,last_frame,first_frame,first_port,first_field,engine_value,replay_value,panic,notes" >>"$csv"
    fi
    echo "$name.slp,$1,5124,$2,$3,$4,$5,$6,$7,,note one; note, with commas" >>"$csv"
}

case "$name" in
match)
    row match 5000 "" "" "" "" ""
    echo "SLPCHECK $name.slp match"
    exit 0
    ;;
diverged)
    row diverged 5000 797 2 x 12.5 13.0
    echo "SLPCHECK $name.slp diverged"
    exit 0
    ;;
early)
    row match 1000 "" "" "" "" ""
    exit 0
    ;;
panic)
    echo "thread 'main' panicked at engine.rs:1" >&2
    exit 134
    ;;
nodisc)
    row "boot-error: <disc>: No such file or directory (os error 2)" "" "" "" "" "" ""
    echo "SLPCHECK $name.slp boot-error: <disc>: No such file or directory (os error 2)"
    exit 1
    ;;
usage)
    echo "error: cannot read the replay" >&2
    exit 2
    ;;
hang)
    exec sleep 30
    ;;
envcheck)
    if [ "$SLPCHECK_VANILLA_SPAWN" != "1" ]; then
        echo "SLPCHECK_VANILLA_SPAWN is not set" >&2
        exit 2
    fi
    row match 5000 "" "" "" "" ""
    exit 0
    ;;
noenvcheck)
    if [ -n "${SLPCHECK_VANILLA_SPAWN+set}" ]; then
        echo "SLPCHECK_VANILLA_SPAWN is set" >&2
        exit 2
    fi
    row match 5000 "" "" "" "" ""
    exit 0
    ;;
*)
    echo "fake_slpcheck.sh: no behaviour for $name" >&2
    exit 2
    ;;
esac
